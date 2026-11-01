//! The slot host: which view draws each swappable part of the shell.
//!
//! A part of the shell is a slot, and the person binds each slot to a view in
//! `settings:shell-views` — an app and one of the views it publishes. The
//! session starts that view where it puts the part up, in the place of the
//! one it ships with, and starts another when the person binds another: the
//! part is up before and after, and the reading behind it never moves.
//!
//! What a slot's view is bound to is read from the settings resource the
//! settings service publishes, which is the same reading a person, an agent
//! or the Settings window sees. Nothing is bound by default: a slot the
//! person left alone is drawn by the view the session ships with, and a
//! binding whose app this machine does not have leaves it at that, rather
//! than drawing nothing.
//!
//! The parts the system draws itself ([`settings_sections::shell_views::FIXED`])
//! are not here: the settings resource refuses to bind them.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use serde_json::Value;
use settings_store::device_class::DeviceClass;

use crate::surfaces;

/// The resource that binds a slot to a view.
pub const BINDINGS: &str = "settings:shell-views";

/// The view bound to a slot: an app, and the view it publishes.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Binding {
    pub app: String,
    pub view: String,
}

impl Binding {
    /// The binding `json` names, when it names both an app and a view.
    fn of(json: &Value) -> Option<Self> {
        let app = json.get("app")?.as_str()?.trim();
        let view = json.get("view")?.as_str()?.trim();
        (!app.is_empty() && !view.is_empty()).then(|| Self {
            app: app.to_owned(),
            view: view.to_owned(),
        })
    }

    /// The program that runs this view, as the session finds its own parts:
    /// `so.tilo.window-menu` is `tilo-window-menu`.
    #[must_use]
    pub fn program(&self) -> String {
        let trimmed = self.app.strip_prefix("so.tilo.").unwrap_or(&self.app);
        format!("tilo-{}", trimmed.replace('.', "-"))
    }
}

/// What the session is to do with a slot this pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mount {
    /// Nothing: the view bound to it is the one it runs, or the part is not up.
    Nothing,
    /// Start `program` for the slot, after stopping what ran it (`restart`).
    Start { program: PathBuf, restart: bool },
    /// Stop what runs for the slot: the part is not up any more.
    Stop,
}

/// What each slot is bound to, and what the session started for it.
#[derive(Debug, Default)]
pub struct Slots {
    /// What each slot runs, so a binding that did not move is not looked up
    /// again.
    running: BTreeMap<String, Binding>,
    /// Bindings whose program this machine does not have: the slot stays with
    /// the view the session ships with, and the miss is not retried every
    /// pass.
    absent: BTreeSet<Binding>,
}

impl Slots {
    /// The binding that draws `slot` now: the person's, when `settings` names
    /// one for this device, else the one the session ships (`shipped`).
    fn bound(slot: &str, shipped: (&str, &str), settings: Option<&Value>) -> Binding {
        settings
            .and_then(|reading| bound_to(reading, slot, DeviceClass::current()))
            .unwrap_or_else(|| Binding {
                app: shipped.0.to_owned(),
                view: shipped.1.to_owned(),
            })
    }

    /// Follow `slot`: whether its part is `up`, and the view bound to it now.
    pub fn follow(
        &mut self,
        slot: &str,
        shipped: (&str, &str),
        up: bool,
        settings: Option<&Value>,
    ) -> Mount {
        if !up {
            return if self.running.remove(slot).is_some() {
                Mount::Stop
            } else {
                Mount::Nothing
            };
        }
        let bound = Self::bound(slot, shipped, settings);
        let shipped = Self::bound(slot, shipped, None);
        // What the person bound runs here; one this machine does not have
        // leaves the slot to the view the shell ships with, and the slot is
        // that view's as long as the binding goes on missing.
        let view = if self.absent.contains(&bound) {
            &shipped
        } else {
            &bound
        };
        if self.running.get(slot) == Some(view) {
            return Mount::Nothing;
        }
        // A view bound now that is not the one that drew the slot takes its
        // place, over the reading the slot already shows.
        let restart = self.running.remove(slot).is_some();
        let started = match self.program(slot, view) {
            Some(program) => Some((program, view.clone())),
            None if view != &shipped => self
                .program(slot, &shipped)
                .map(|program| (program, shipped.clone())),
            None => None,
        };
        match started {
            Some((program, started)) => {
                self.running.insert(slot.to_owned(), started);
                Mount::Start { program, restart }
            }
            None => {
                if restart {
                    Mount::Stop
                } else {
                    Mount::Nothing
                }
            }
        }
    }

    /// The program that runs `binding` where the session puts it up, or `None`
    /// when this machine has none: a binding that does not run is not looked
    /// for again, so a slot nobody draws costs one look.
    fn program(&mut self, slot: &str, binding: &Binding) -> Option<PathBuf> {
        if self.absent.contains(binding) {
            return None;
        }
        match surfaces::program_path(&binding.program()) {
            Ok(program) => Some(program),
            Err(error) => {
                tracing::debug!(
                    slot,
                    app = %binding.app,
                    %error,
                    "the view bound to this slot is not installed here"
                );
                self.absent.insert(binding.clone());
                None
            }
        }
    }

    /// The view that drew `slot` could not start here: the slot keeps what it
    /// had, and this binding is not tried again.
    pub fn failed(&mut self, slot: &str, binding: Binding) {
        self.running.remove(slot);
        self.absent.insert(binding);
    }

    /// What draws `slot` now, once the session started it.
    #[must_use]
    pub fn running(&self, slot: &str) -> Option<&Binding> {
        self.running.get(slot)
    }
}

/// The view `reading` binds to `slot` on `device`: the one bound for that kind
/// of device, else the one bound everywhere.
#[must_use]
pub fn bound_to(reading: &Value, slot: &str, device: DeviceClass) -> Option<Binding> {
    let class = settings_store::shell_views::device_key(device);
    reading
        .get("on_device")
        .and_then(|devices| devices.get(class))
        .and_then(|bindings| bindings.get(slot))
        .or_else(|| {
            reading
                .get("bindings")
                .and_then(|bindings| bindings.get(slot))
        })
        .and_then(Binding::of)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::json;

    use super::{Binding, Mount, Slots, bound_to};

    #[test]
    fn a_binding_for_this_kind_of_device_is_the_one_that_draws_it() {
        let reading = json!({
            "bindings": { "window-menu": { "app": "so.tilo.menu", "view": "wide" } },
            "on_device": { "web": { "window-menu": { "app": "so.tilo.menu", "view": "narrow" } } },
        });
        assert_eq!(
            bound_to(
                &reading,
                "window-menu",
                settings_store::device_class::DeviceClass::Native
            )
            .map(|binding| binding.view),
            Some("wide".to_owned())
        );
        assert_eq!(
            bound_to(
                &reading,
                "window-menu",
                settings_store::device_class::DeviceClass::Web
            )
            .map(|binding| binding.view),
            Some("narrow".to_owned())
        );
    }

    #[test]
    fn a_bound_view_this_machine_does_not_have_leaves_the_slot_to_the_shipped_one() {
        let mut env = tilo_test_env::ScopedEnv::new();
        // The view the shell ships with is on this machine; the one the
        // person bound, here, is not.
        env.set("TILO_SURFACE_TILO_SHIPPED", "/bin/sh");
        let shipped = ("so.tilo.shipped", "shipped");
        let settings = json!({
            "bindings": { "menu": { "app": "so.tilo.not-installed", "view": "menu" } },
        });
        let mut slots = Slots::default();
        assert_eq!(
            slots.follow("menu", shipped, true, Some(&settings)),
            Mount::Start {
                program: PathBuf::from("/bin/sh"),
                restart: false,
            }
        );
        // And it stays that view's: the missing binding is not tried again,
        // so the slot is not started over on every pass.
        assert_eq!(
            slots.follow("menu", shipped, true, Some(&settings)),
            Mount::Nothing
        );
        assert_eq!(
            slots.running("menu").map(|running| running.app.as_str()),
            Some("so.tilo.shipped")
        );
    }

    #[test]
    fn a_view_runs_as_its_apps_program() {
        assert_eq!(
            Binding {
                app: "so.tilo.window-menu".into(),
                view: "window-menu".into()
            }
            .program(),
            "tilo-window-menu"
        );
    }
}
