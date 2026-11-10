//! The slot host: which view draws each swappable part of the shell.
//! A part of the shell is a slot, and the person binds each slot to a view in
//! `settings:shell-views` — an app and one of the views it publishes. The session starts that view
//! in the place of the one it ships with, and starts another when the person binds another: the
//! view runs for the session's life, drawing the reading the session publishes for the slot, so
//! opening the part costs no start.
//! What a slot's view is bound to is read from the settings resource the settings service
//! publishes, which is the same reading a person, an agent or the Settings window sees. Nothing is
//! bound by default: a slot the person left alone is drawn by the view the session ships with, and
//! a binding whose app this machine does not have leaves it at that, rather than drawing nothing.
//! The parts the system draws itself ([`settings_sections::shell_views::FIXED`]) are not here: the
//! settings resource refuses to bind them.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::Value;
use settings_store::device_class::DeviceClass;
use tilo_surface_host::slot_view::SlotSurface;

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

    /// The program that runs this view, as the session finds its own parts: `so.tilo.window-menu`
    /// is `tilo-window-menu` .
    #[must_use]
    pub fn program(&self) -> String {
        let trimmed = self.app.strip_prefix("so.tilo.").unwrap_or(&self.app);
        format!("tilo-{}", trimmed.replace('.', "-"))
    }
}

/// What draws a slot: a program the session runs as a process of its own, or a view written in
/// tilolang, which it runs in its own process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum View {
    /// A surface app: started with the slot's name, drawing the slot from its own process.
    Program(PathBuf),
    /// A tilolang package's view, run by the session over the slot's reading.
    Package {
        /// The package's payload: `app.wasm` , `app.ids` and the modules it uses
        /// (`tilolang_tilo::bundle`).
        dir: PathBuf,
        /// The app the person bound, as the settings reading names it.
        app: String,
        /// The view it publishes that draws the slot.
        view: String,
    },
}

/// What the session is to do with a slot this pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mount {
    /// Nothing: the view bound to it is the one it runs.
    Nothing,
    /// Start `view` for the slot, after stopping what ran it (`restart`).
    Start { view: View, restart: bool },
    /// Stop what runs for the slot: the view bound to it now is not on this machine.
    Stop,
}

/// What each slot is bound to, and what the session started for it.
#[derive(Debug, Default)]
pub struct Slots {
    /// What each slot runs, so a binding that did not move is not looked up again.
    running: BTreeMap<String, Binding>,
    /// Bindings whose program this machine does not have: the slot stays with the view the session
    /// ships with, and the miss is not retried every pass.
    absent: BTreeSet<Binding>,
    /// Slots whose view has been put up once in this session. A view runs for the session's life,
    /// so the part it draws comes up with it already there; one whose program is not on this
    /// machine is not looked for again on every pass while the part is away.
    stood: BTreeSet<String>,
}

impl Slots {
    /// The binding that draws `slot` now: the person's, when `settings` names one for this device,
    /// else the one the session ships (`shipped`).
    fn bound(slot: &str, shipped: (&str, &str), settings: Option<&Value>) -> Binding {
        settings
            .and_then(|reading| bound_to(reading, slot, DeviceClass::current()))
            .unwrap_or_else(|| Binding {
                app: shipped.0.to_owned(),
                view: shipped.1.to_owned(),
            })
    }

    /// Follow `slot` : the view bound to it now, which runs for the session's life so every time
    /// the part comes up the view already stands there.
    pub fn follow(&mut self, slot: &str, shipped: (&str, &str), settings: Option<&Value>) -> Mount {
        self.stood.insert(slot.to_owned());
        let bound = Self::bound(slot, shipped, settings);
        let shipped = Self::bound(slot, shipped, None);
        // What the person bound runs here; one this machine does not have leaves the slot to the
        // view the shell ships with, and the slot is that view's as long as the binding goes on
        // missing.
        let view = if self.absent.contains(&bound) {
            &shipped
        } else {
            &bound
        };
        if self.running.get(slot) == Some(view) {
            return Mount::Nothing;
        }
        // A view bound now that is not the one that drew the slot takes its place, over the reading
        // the slot already shows.
        let restart = self.running.remove(slot).is_some();
        let started = match self.view_of(slot, view) {
            Some(started) => Some((started, view.clone())),
            None if view != &shipped => self
                .view_of(slot, &shipped)
                .map(|started| (started, shipped.clone())),
            None => None,
        };
        match started {
            Some((view, started)) => {
                self.running.insert(slot.to_owned(), started);
                Mount::Start { view, restart }
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

    /// The view that draws `binding` where the session puts it up, or `None` when this machine has
    /// none: a binding that does not run is not looked for again, so a slot nobody draws costs one
    /// look.
    fn view_of(&mut self, slot: &str, binding: &Binding) -> Option<View> {
        if self.absent.contains(binding) {
            return None;
        }
        // An app this machine installed as a tilolang package is run here,
        // over the reading of the slot; anything else the binding names is a
        // program of its own.
        if let Some(package) = installed(binding) {
            return Some(package);
        }
        match surfaces::program_path(&binding.program()) {
            Ok(path) => Some(package_at(&path, binding).unwrap_or(View::Program(path))),
            Err(error) => {
                // ignored-failure: a slot whose view is not installed shows nothing, and the next publication binds it
                // The slot has no view: the menu it stands for is up in the
                // session's reading and nothing draws it, so this says so
                // where a person reading the log finds it, not at a level
                // nobody turns on.
                tracing::warn!(
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

    /// What drew `slot` ended on its own: nothing runs for it any more, so the next time the part
    /// comes up the view is started again rather than trusted to a process that is gone.
    pub fn forget(&mut self, slot: &str) {
        self.running.remove(slot);
    }

    /// The view that drew `slot` could not start here: the slot keeps what it had, and this binding
    /// is not tried again.
    pub fn failed(&mut self, slot: &str, binding: Binding) {
        self.running.remove(slot);
        self.absent.insert(binding);
    }

    /// What draws `slot` now, once the session started it.
    #[must_use]
    pub fn running(&self, slot: &str) -> Option<&Binding> {
        self.running.get(slot)
    }

    /// Whether `slot` 's view has been put up once in this session.
    #[must_use]
    pub fn stood(&self, slot: &str) -> bool {
        self.stood.contains(slot)
    }
}

/// The tilolang package `binding` names, when this machine installed it as an app: its payload
/// holds the compiled module.
fn installed(binding: &Binding) -> Option<View> {
    let app = app_catalogue::apps::read_one_from_env(&binding.app)
        .ok()
        .flatten()?;
    package_at(&app.payload_root, binding)
}

/// The package in `dir` , when `dir` holds a compiled tilolang module: a view of a slot written in
/// tilolang. `None` for a directory of anything else, and for a surface app, which the session runs
/// as a program.
fn package_at(dir: &Path, binding: &Binding) -> Option<View> {
    dir.join(tilolang_tilo::bundle::WASM_FILE)
        .is_file()
        .then(|| View::Package {
            dir: dir.to_path_buf(),
            app: binding.app.clone(),
            view: binding.view.clone(),
        })
}

/// The state `slot` publishes, as a view of it written in tilolang reads it: the resource, and the
/// surface a package drawing the slot is compiled against. `None` for a slot whose state a view
/// does not read.
#[must_use]
pub fn surface(slot: &str) -> Option<SlotSurface> {
    match slot {
        crate::shell::state::WINDOW_MENU_SLOT => Some(SlotSurface {
            id: crate::shell::state::WINDOW_MENU.to_owned(),
            kind: crate::shell::state::WINDOW_MENU_KIND.to_owned(),
            app: crate::shell::state::WINDOW_MENU_APP.to_owned(),
            title: "Window menu".to_owned(),
            text: crate::shell::state::WINDOW_MENU_SURFACE.to_owned(),
        }),
        _ => None,
    }
}

/// The view `reading` binds to `slot` on `device` : the one bound for that kind of device, else the
/// one bound everywhere.
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

    use super::{Binding, Mount, Slots, View, bound_to};

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
        // The view the shell ships with is on this machine; the one the person bound, here, is not.
        env.set("TILO_SURFACE_TILO_SHIPPED", "/bin/sh");
        let shipped = ("so.tilo.shipped", "shipped");
        let settings = json!({
            "bindings": { "menu": { "app": "so.tilo.not-installed", "view": "menu" } },
        });
        let mut slots = Slots::default();
        assert_eq!(
            slots.follow("menu", shipped, Some(&settings)),
            Mount::Start {
                view: View::Program(PathBuf::from("/bin/sh")),
                restart: false,
            }
        );
        // And it stays that view's: the missing binding is not tried again,
        // so the slot is not started over on every pass.
        assert_eq!(
            slots.follow("menu", shipped, Some(&settings)),
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
