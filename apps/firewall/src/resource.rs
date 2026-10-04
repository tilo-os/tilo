//! Firewall as a [`tilo_resource::Resource`]: the stored configuration is
//! the shared document, and firewalld on each device is its projection.
//!
//! Every edit — a rule, a service, the active zone, start/stop, panic — is a
//! [`FirewallAction`] through the framework dispatcher. After every change,
//! each device running the app applies the stored configuration to its own
//! firewalld (a projection drain); what it applied last is that device's own
//! setting, so an unchanged configuration is not pushed again. Applying now
//! and reading the configuration firewalld already has are effects with no
//! document delta of their own: reading one in submits [`FirewallAction::Import`].

use std::cell::RefCell;
use std::rc::Rc;

use serde::{Deserialize, Serialize};
use ui::framework::{
    EffectCx, EffectStatus, Effected, ResourceApp, ResourceSurface, SeedCx, Setting, SurfaceCx,
    ViewCx,
};
use ui::prelude::Effect;
use ui::storage::{Scope, SettingsSchema};
use ui::tilo_resource::{self, ActionDecl, Doc, Invoke, ModelError, Resource, SearchEntry};
use ui::widget::Element;

use crate::actions::{FIREWALL_ACTION_TYPE, FirewallAction, register};
use crate::app::FirewallApp;
use crate::backend::{Backend, Mode};
use crate::doc::FirewallDoc;
use crate::model::{FirewallConfig, ZoneKind};

/// The firewall configuration resource.
pub struct Firewall;

impl Resource for Firewall {
    const KIND: &'static str = "firewall.config";
    const LABEL: &'static str = "Firewall";
    const ACTION_TYPE: &'static str = FIREWALL_ACTION_TYPE;

    type State = FirewallConfig;
    type Action = FirewallAction;

    fn register(registry: &mut tilo_actions::ActionRegistry) {
        register(registry);
    }

    fn read(doc: &Doc) -> Result<FirewallConfig, ModelError> {
        Ok(FirewallDoc::load(doc).unwrap_or_else(FirewallConfig::empty))
    }

    /// Start/stop and panic carry the stored on/off.
    fn bound_declarations(state: &FirewallConfig) -> Vec<ActionDecl<Self>> {
        if state.zones.is_empty() {
            return Vec::new();
        }
        crate::actions::toggle_declarations(state)
    }

    fn children(state: &FirewallConfig) -> Vec<tilo_actions::Resource> {
        crate::actions::zone_children(state)
    }

    fn search(state: &FirewallConfig) -> Vec<SearchEntry> {
        state
            .zones
            .iter()
            .map(|zone| SearchEntry {
                anchor: Some(format!("firewall.zone:{}", zone.kind.name())),
                title: zone.kind.name().to_owned(),
                text: format!("{} rules", zone.rules.len()),
                action: None,
            })
            .collect()
    }
}

/// What the firewall does outside the document, for every caller.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "firewall.effect")]
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

/// The status line one apply leaves.
pub(crate) fn applied_status(applied: &Applied) -> String {
    match applied {
        Ok((Mode::Live, n)) => format!("Applied {n} firewall-cmd commands"),
        Ok((Mode::DryRun, n)) => {
            format!("Dry-run: {n} commands planned (set TILO_FIREWALL_APPLY=1)")
        }
        Err(error) => format!("Apply failed: {error}"),
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
            Err("firewalld returned unsupported or unsafe rule data; no changes were made".into())
        }
        None => Err(
            "Still unable to reach firewalld. Start the service, authorize it, then retry.".into(),
        ),
    }
}

/// Framework app: the firewall widget over [`Firewall`].
pub struct FirewallShell;

/// Every screenshot scenario. All but `service-error` show the sample
/// ruleset; the rest is view state the surface stages.
const SCENARIOS: &[&str] = &[
    "default",
    "empty",
    "error",
    "confirmation",
    "active-change",
    "service-error",
    "services",
    "selected",
];

impl ResourceApp for FirewallShell {
    type Resource = Firewall;
    const ID: &'static str = "so.tilo.firewall";
    const TITLE: &'static str = "Firewall";
    const ICON: Option<&'static str> = Some("firewall");

    fn view(cx: &mut ViewCx<Firewall>) -> Element {
        cx.surface(FirewallApp::new())
    }

    fn scenarios() -> &'static [&'static str] {
        SCENARIOS
    }

    fn shot_viewport(_scenario: Option<&str>) -> (u32, u32) {
        (1100, 720)
    }

    /// The bundled sample ruleset, never the capturing machine's; the
    /// active-change scenario has just made `home` the active zone.
    fn scenario(name: Option<&str>) -> Vec<Invoke<Firewall>> {
        if name == Some("service-error") {
            return Vec::new();
        }
        let mut actions = vec![FirewallAction::Import {
            config: crate::sample::sample_config(),
        }];
        if name == Some("active-change") {
            actions.push(FirewallAction::SetDefaultZone {
                zone: ZoneKind::Home,
            });
        }
        actions
            .iter()
            .filter_map(|action| Invoke::apply(action).ok())
            .collect()
    }

    /// A new record starts with what firewalld on the creating device
    /// already has, which that device therefore need not apply again. A
    /// device without a readable firewalld starts empty and offers a retry.
    fn seed(cx: &SeedCx<'_>) -> Vec<Invoke<Firewall>> {
        if cx.host().capture().is_some() {
            return Vec::new();
        }
        let Ok(config) = read_device() else {
            return Vec::new();
        };
        let applied = AppliedConfig {
            config: Some(config.canonical().to_json()),
        };
        if let Err(error) = cx.host().settings::<AppliedConfig>().set(&applied) {
            tracing::warn!(%error, "could not remember the imported firewall configuration");
        }
        Invoke::apply(&FirewallAction::Import { config })
            .into_iter()
            .collect()
    }
}

impl ResourceSurface<Firewall, FirewallEffect> for FirewallApp {
    fn present(&mut self, state: &Rc<FirewallConfig>) {
        self.show_config(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Firewall>) {
        let applied = cx.settings::<AppliedConfig>();
        self.attach_framework(cx.dispatcher(), applied.clone());
        // Apply the stored configuration to this device after every change.
        let outcome = self.apply_outcome();
        let pending = applied.clone();
        cx.drain(
            move |config: &FirewallConfig| pending_apply(config, &pending.get()),
            move |config: FirewallConfig, cx| {
                let applied = applied.clone();
                let outcome = Rc::clone(&outcome);
                Ok(cx.thread(
                    move |_| {
                        let result = apply_on_device(&config);
                        (config, result)
                    },
                    move |(), (config, result): (FirewallConfig, Applied), _| {
                        let status = applied_status(&result);
                        *outcome.borrow_mut() = Some(status.clone());
                        result.map_err(|_| status)?;
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
                let config = cx.live().get().as_ref().clone();
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
                    cx.apply(&FirewallAction::Import { config });
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

/// Where the projection's result goes until the surface shows it.
pub(crate) type ApplyOutcome = Rc<RefCell<Option<String>>>;

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
            &FirewallConfig::empty(),
        );
        assert!(
            !empty
                .actions
                .iter()
                .any(|action| action.id == "toggle-enabled"),
            "nothing to start or stop before there is a configuration"
        );

        let mut app = TestApp::<FirewallShell>::new().unwrap();
        app.invoke(
            Invoke::apply(&FirewallAction::Import {
                config: sample_config(),
            })
            .unwrap(),
        )
        .unwrap();
        let surface = app.agent_surface();
        for id in ["toggle-enabled", "toggle-panic"] {
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
        let stored = app.state().to_json();
        let applied = app
            .harness()
            .wait_until(ui::time::Duration::from_secs(30), |_| {
                remembered.get().config.as_deref() == Some(stored.as_str())
            });
        assert!(applied, "the device remembers the configuration it applied");
        let frame = app.harness().inspection_json().unwrap();
        assert!(frame.contains("commands planned"), "{frame}");

        // Stopping the firewall is an edit like any other: confirmed, in
        // the document, and applied by the device in turn.
        app.invoke_confirmed(Invoke::apply(&FirewallAction::SetEnabled { on: false }).unwrap())
            .unwrap();
        assert!(!app.state().enabled);
        let stopped = app.state().to_json();
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
        assert_eq!(active.harness().inspection().viewport, [1100, 720]);
        assert_eq!(active.state().default_zone, ZoneKind::Home);
        let frame = active.harness().inspection_json().unwrap();
        assert!(frame.contains("home is now the active zone"), "{frame}");

        let mut error = TestApp::<FirewallShell>::capture(Some("service-error")).unwrap();
        assert!(error.state().zones.is_empty());
        let frame = error.harness().inspection_json().unwrap();
        assert!(frame.contains("Permission denied"), "{frame}");
    }
}
