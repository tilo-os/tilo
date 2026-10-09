//! Firewall as a resource: the stored configuration ([`Firewall`]) is the
//! shared document, and firewalld on each device is its projection.
//!
//! A zone's policy, members, rules and the active zone are field edits at
//! their paths; starting, stopping and panic mode are verbs, the switches a
//! running firewall is turned with; deleting a rule is a verb
//! because the deletion must win over a concurrent edit; taking in what
//! firewalld reported is the `import` verb. After every change, each device
//! running the app applies the stored configuration to its own firewalld (a
//! projection drain); what it applied last is that device's own setting, so
//! an unchanged configuration is not pushed again.

use std::cell::RefCell;
use std::rc::Rc;

use serde::{Deserialize, Serialize};
use ui::framework::{
    EffectCx, EffectStatus, Effected, ResourceApp, ResourceSurface, SeedCx, Setting, SurfaceCx,
    ViewCx,
};
use ui::prelude::Effect;
use ui::storage::{Scope, SettingsSchema};
use ui::tilo_resource::{self, Availability, Derived, Id, Invoke, Refusal, SearchEntry, resource};
use ui::widget::Element;

use crate::app::{FirewallApp, Notice, Tone};
use crate::backend::{Backend, Mode};
pub use crate::model::Firewall;
use crate::model::{FirewallConfig, ZoneKind};

#[resource(
    kind = "firewall.config",
    label = "Firewall",
    available = Firewall::available,
    children = Firewall::zone_places,
    search = Firewall::search,
)]
impl Firewall {
    /// Start firewall.
    #[action]
    pub fn start(&mut self) {
        self.enabled = true;
    }

    /// Stop firewall.
    #[action]
    pub fn stop(&mut self) {
        self.enabled = false;
    }

    /// Enter panic mode: drop all traffic in and out.
    #[action]
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
        #[param("The zone's name, such as public")] zone: String,
        #[param("The rule's id")] rule: Id,
    ) -> Result<(), Refusal> {
        let stored = self.zones.get_mut(&zone).ok_or("there is no such zone")?;
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
                    .with(Self::DELETE_RULE.bind(
                        firewall::DeleteRule::paths().zone,
                        zone.kind.name().to_owned(),
                    ))
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
#[effect(operation = "firewall.effect", device_only)]
pub enum FirewallEffect {
    /// Apply firewall configuration.
    #[effect(job, permission = "admin")]
    ApplyConfig,
    /// Retry firewalld connection.
    #[effect(job)]
    Retry,
}

/// What this device last pushed to (or read from) its firewalld, so the
/// projection applies a configuration once rather than on every launch.
/// Never leaves the device: another device's firewalld is its own.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppliedConfig {
    /// The applied configuration in canonical JSON.
    pub config: Option<String>,
}

impl SettingsSchema for AppliedConfig {
    const SCOPE: Scope = Scope::DeviceLocal;
}

/// The stored configuration still to apply on this device, keyed by its
/// canonical form: nothing while there is none, while it is invalid, or once
/// this device applied exactly it.
pub(crate) fn pending_apply(
    config: &FirewallConfig,
    applied: &AppliedConfig,
) -> Vec<(String, FirewallConfig)> {
    if config.zones.is_empty() || config.validate().is_err() {
        return Vec::new();
    }
    let canonical = config.to_json();
    if applied.config.as_deref() == Some(canonical.as_str()) {
        return Vec::new();
    }
    vec![(canonical, config.clone())]
}

/// How one apply ended: the backend's mode and how many commands it ran or
/// planned, or why it failed.
pub(crate) type Applied = Result<(Mode, usize), String>;

/// Push `config` to this device's firewalld (or only plan it, unless the
/// live apply is armed). Blocking: runs off the UI thread.
pub(crate) fn apply_on_device(config: &FirewallConfig) -> Applied {
    let mut backend = Backend::detect();
    backend
        .apply(config)
        .map(|()| (backend.mode(), backend.last_plan().len()))
}

/// What one apply reports.
pub(crate) fn applied_status(applied: &Applied) -> Notice {
    let changes = |n: usize| if n == 1 { "change" } else { "changes" };
    match applied {
        Ok((Mode::Live, n)) => Notice {
            text: format!("Applied {n} {}", changes(*n)),
            tone: Tone::Success,
        },
        Ok((Mode::DryRun, n)) => Notice {
            text: format!(
                "{n} {} checked. This device does not allow applying them.",
                changes(*n)
            ),
            tone: Tone::Info,
        },
        Err(error) => Notice {
            text: format!("Could not apply the configuration: {error}"),
            tone: Tone::Error,
        },
    }
}

/// Remember that this device's firewalld holds `config`.
pub(crate) fn remember_applied(applied: &Setting<AppliedConfig>, config: &FirewallConfig) {
    let config = Some(config.to_json());
    if let Err(error) = applied.set(AppliedConfig { config }) {
        tracing::warn!(%error, "could not remember the applied firewall configuration");
    }
}

/// Read this device's firewalld; `Err` says why it cannot be shown.
fn read_device() -> Result<FirewallConfig, String> {
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
/// ruleset; the rest is view state the surface stages.
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
    "selected",
    "narrow",
    "drawer",
];

impl ResourceApp for FirewallShell {
    type Resource = Firewall;
    const ID: &'static str = "so.tilo.firewall";
    const TITLE: &'static str = "Firewall";
    const ICON: Option<&'static str> = Some("firewall");

    fn view(cx: &mut ViewCx<Firewall>) -> Element {
        crate::locales::install();
        cx.surface(FirewallApp::new())
    }

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
    /// already has, which that device therefore need not apply again. A
    /// device without a readable firewalld starts empty and offers a retry,
    /// and so does a record created without a window: it is no device's.
    fn seed(cx: &SeedCx<'_>) -> Vec<Invoke<Firewall>> {
        let Some(host) = cx.host().filter(|host| host.capture().is_none()) else {
            return Vec::new();
        };
        let Ok(config) = read_device() else {
            return Vec::new();
        };
        let applied = AppliedConfig {
            config: Some(config.canonical().to_json()),
        };
        if let Err(error) = host.settings::<AppliedConfig>().set(&applied) {
            tracing::warn!(%error, "could not remember the imported firewall configuration");
        }
        vec![Firewall::import_of(&config)]
    }
}

impl ResourceSurface<Firewall, FirewallEffect> for FirewallApp {
    fn present(&mut self, state: &Rc<Firewall>) {
        self.show_config(&state.config());
    }

    fn attach(&mut self, cx: SurfaceCx<Firewall>) {
        let applied = cx.settings::<AppliedConfig>();
        self.attach_framework(cx.dispatcher(), applied.clone());
        // Apply the stored configuration to this device after every change.
        let outcome = self.apply_outcome();
        let pending = applied.clone();
        cx.drain(
            move |record: &Firewall| pending_apply(&record.config(), &pending.get()),
            move |config: FirewallConfig, cx| {
                let applied = applied.clone();
                let outcome = Rc::clone(&outcome);
                Ok(cx.thread(
                    move |_| {
                        let result = apply_on_device(&config);
                        (config, result)
                    },
                    move |(), (config, result): (FirewallConfig, Applied), _| {
                        if result.is_err() {
                            let status = applied_status(&result);
                            let message = status.text.clone();
                            *outcome.borrow_mut() = Some(status);
                            return Err(message);
                        }
                        remember_applied(&applied, &config);
                        Ok(Effected::done())
                    },
                ))
            },
        );
    }

    fn stage(&mut self, scenario: Option<&str>) {
        if let Some(name) = scenario {
            self.pose_fixture(name);
        }
    }

    fn effect(
        &mut self,
        effect: FirewallEffect,
        cx: &mut EffectCx<Firewall, Self>,
    ) -> Result<Effected, String> {
        match effect {
            FirewallEffect::ApplyConfig => {
                let config = cx.live().get().config();
                if config.zones.is_empty() {
                    return Err("Firewall service is unavailable".into());
                }
                Ok(cx.thread(
                    move |_| {
                        let result = apply_on_device(&config);
                        (config, result)
                    },
                    |app: &mut Self, (config, result): (FirewallConfig, Applied), _| {
                        app.finish_apply(&config, &result);
                        result.map(|_| Effected::done())
                    },
                ))
            }
            FirewallEffect::Retry => Ok(cx.thread(
                |_| read_device(),
                |app: &mut Self, read: Result<FirewallConfig, String>, cx| {
                    let config = app.finish_retry(read)?;
                    cx.invoke(Firewall::import_of(&config));
                    Ok(Effected::done())
                },
            )),
        }
    }

    fn effects(&self) -> Vec<EffectStatus> {
        if self.is_available() {
            vec![EffectStatus::unavailable(
                FirewallEffect::RETRY,
                "connected",
                "The firewall configuration is already loaded",
            )]
        } else {
            vec![EffectStatus::unavailable(
                FirewallEffect::APPLY_CONFIG,
                "unavailable",
                "Firewall service is unavailable",
            )]
        }
    }
}

/// Where a failed apply of the projection goes until the surface reports it.
pub(crate) type ApplyOutcome = Rc<RefCell<Option<Notice>>>;

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

    /// The configuration still to apply is the stored one until this device
    /// applied exactly it; an empty or invalid one is never applied.
    #[test]
    fn a_device_applies_each_stored_configuration_once() {
        let config = sample_config().canonical();
        let fresh = AppliedConfig::default();
        let pending = pending_apply(&config, &fresh);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].1, config);

        let applied = AppliedConfig {
            config: Some(config.to_json()),
        };
        assert!(pending_apply(&config, &applied).is_empty());

        let mut changed = config.clone();
        changed.panic_mode = true;
        assert_eq!(pending_apply(&changed, &applied).len(), 1);

        assert!(pending_apply(&FirewallConfig::empty(), &fresh).is_empty());
        let mut invalid = config;
        invalid.zones[0].services.push("bad service".into());
        assert!(pending_apply(&invalid, &fresh).is_empty());
    }

    /// The published root offers start/stop and panic as document edits
    /// bound to the stored state, and apply/retry as effects whose
    /// availability follows whether there is a configuration. Once there
    /// is one, this device applies it (only planned here: the live apply is
    /// not armed) and remembers that it did.
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

        let remembered = app.settings::<AppliedConfig>();
        let stored = app.state().config().to_json();
        let applied = app
            .harness()
            .wait_until(ui::time::Duration::from_secs(30), |_| {
                remembered.get().config.as_deref() == Some(stored.as_str())
            });
        assert!(applied, "the device remembers the configuration it applied");

        // Stopping the firewall is an edit like any other: confirmed, in
        // the document, and applied by the device in turn.
        app.invoke_confirmed(Firewall::STOP.invoke()).unwrap();
        assert!(!app.state().enabled);
        let stopped = app.state().config().to_json();
        let reapplied = app
            .harness()
            .wait_until(ui::time::Duration::from_secs(30), |_| {
                remembered.get().config.as_deref() == Some(stopped.as_str())
            });
        assert!(reapplied, "the edit reached this device's firewall");
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
        a.apply(Firewall::DELETE_RULE.with((z.name().to_owned(), Firewall::rule(ids[0]))))
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
        app.call(
            "field-edit",
            serde_json::json!({ "edit": Firewall::paths().default_zone.set(ZoneKind::Home) }),
        )
        .unwrap();
        let catalogue =
            tilo_history::Catalogue::new("en-US", &[include_str!("../locales/en-US.ftl")]);
        let reader = tilo_history::Reader::new("en-US").with_app(&catalogue);
        let sentences: Vec<String> = app
            .versions()
            .into_iter()
            .filter_map(|version| version.context)
            .filter(|entry| entry.what.is_some())
            .map(|entry| reader.sentence(&entry))
            .collect();
        assert_eq!(
            sentences.last().map(String::as_str),
            Some("Made home the active zone")
        );
    }
}
