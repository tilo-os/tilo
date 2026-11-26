//! The platform's roots one window reads, followed from where the session publishes them: the
//! platform packages its bundle `use`s ([`crate::platform`]) loaded into its host, each root they
//! name followed through the process's [`PublishedSource`], and the app's own preferences, which
//! the settings service keeps. The host hands their readings in
//! ([`crate::instance::Instance::set_reading`]) and answers their actions by asking the session,
//! so a view of them never holds a copy.

use crate::platform::{load, platform_packages, platform_root_defaults, prefs_address};

/// One published resource, followed: its reading each time the session publishes one.
pub trait Reading: Send {
    /// The newest reading since the last call, `None` while nothing new came.
    fn latest(&self) -> Option<Result<String, String>>;
}

/// Where the platform's published resources are reached: the session's services, which a
/// process reaches over its connection.
pub trait PublishedSource: Send + Sync {
    /// Follow the reading of the published resource `resource`, waking `wake`'s owner at each
    /// new one.
    ///
    /// # Errors
    /// No session's services are reached, or the watch was refused.
    fn follow(
        &self,
        resource: &str,
        wake: tilo_rt::wake::WakeHandle,
    ) -> Result<Box<dyn Reading>, String>;

    /// Ask the published resource `resource` to run `action` with `arguments`, as the session
    /// serves it, and hand back what it said: a refusal is its words.
    ///
    /// # Errors
    /// No session's services are reached, or the change was refused or cancelled.
    fn invoke(
        &self,
        resource: &str,
        action: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), String>;
}

static SOURCE: std::sync::OnceLock<Box<dyn PublishedSource>> = std::sync::OnceLock::new();

/// Read the platform's published resources through `source` from now on. Called once, by the
/// process's start; a process with none reads no platform root.
pub fn install(source: Box<dyn PublishedSource>) {
    let _ = SOURCE.set(source);
}

fn published() -> Option<&'static dyn PublishedSource> {
    SOURCE.get().map(Box::as_ref)
}

/// The platform's roots one window reads: the packages its bundle `use`s, and each root followed
/// from where the session publishes it.
#[derive(Default)]
pub struct Roots {
    /// The apps among them whose package this window loaded.
    apps: std::collections::BTreeSet<String>,
    /// The roots followed, each with the resource of the app it is.
    followed: Vec<(crate::runtime::ResourceRef, Box<dyn Reading>)>,
    /// The roots a package declares, by id: followed for as long as the window runs. The others
    /// are read on demand — a reading keyed by a range — and only while a view shows one.
    declared: std::collections::BTreeSet<String>,
    /// Wakes the window when a followed root publishes a reading.
    wake: Option<tilo_rt::wake::WakeHandle>,
    /// Whether the packages and their roots were loaded from the bundle.
    loaded: bool,
    /// The app whose window this is, when it declares preferences: the only app's preferences it
    /// reads and changes.
    own_app: Option<String>,
    /// Where this app's preferences are published (`settings/apps/<app>`): the section of the
    /// settings service it reads and sets.
    prefs: Option<String>,
}

/// Why the resource `id` is another app's preferences, which this app neither reads nor changes:
/// its own live at `settings/apps/{app}`. `None` for its own, and for anything else.
#[must_use]
pub fn refuse_foreign_prefs(id: &str, app: &str) -> Option<String> {
    let other = id.split('#').next().unwrap_or(id);
    let theirs = other.strip_prefix("settings/apps/")?;
    (theirs != app).then(|| {
        format!(
            "`{id}` is the preferences of {theirs}: an app reads and changes only its own, at {}",
            prefs_address(app)
        )
    })
}

/// The settings-service call a control's edit of the preference `field` to `value` is: the
/// field's setter (`set-<field>`), with the whole new value. One app's preferences are set field
/// by field, so a bound control's change is the same change Settings makes.
#[must_use]
pub fn prefs_setter(
    field: &str,
    value: &crate::runtime::Value,
) -> (String, serde_json::Map<String, serde_json::Value>) {
    let verb = format!("set-{}", field.replace('_', "-"));
    let value = crate::runtime::value::to_argument(value).unwrap_or_default();
    (
        verb,
        serde_json::Map::from_iter([("value".to_owned(), value)]),
    )
}

impl Roots {
    /// The platform packages `uses` names, loaded into `host`, and each of their roots followed.
    /// What the session does not answer is said once and left reading as it would start.
    pub fn load(
        &mut self,
        host: &crate::host::Host,
        uses: &[String],
        wake: &tilo_rt::wake::WakeHandle,
    ) {
        if self.loaded {
            return;
        }
        self.loaded = true;
        self.wake = Some(wake.clone());
        let apps = load(host.runtime(), uses);
        if published().is_none() {
            self.apps.extend(apps);
            return;
        }
        let Ok(packages) = platform_packages() else {
            return;
        };
        for meta in packages {
            if !apps.contains(&meta.app) {
                continue;
            }
            for root in &meta.named_roots {
                let Some(published) = root.published.as_deref() else {
                    continue;
                };
                // The type a view reads the root as: its app and its name, as the platform's
                // roots name it (`so.tilo.device#Sound`).
                let Some(name) = meta
                    .types
                    .iter()
                    .find(|declared| declared.id == root.ty)
                    .map(|declared| declared.name.clone())
                else {
                    continue;
                };
                let Some(target) = crate::runtime::ResourceRef::parse_id(
                    published,
                    format!("{}#{name}", meta.app),
                ) else {
                    continue;
                };
                self.declared.insert(target.id_string());
                self.follow(&target, wake.clone());
                Self::seed(host, &target);
            }
        }
        self.apps.extend(apps);
    }

    /// Follow the app's own preferences, which the settings service keeps
    /// (`settings/apps/<app>`): each reads as its declared defaults until the service answers,
    /// and every view that reads one runs again when it does. An app that declares none reads
    /// nothing here.
    pub fn load_prefs(
        &mut self,
        host: &crate::host::Host,
        meta: &tilolang::meta::PackageMeta,
        wake: &tilo_rt::wake::WakeHandle,
    ) {
        if meta.prefs.is_empty() {
            return;
        }
        let address = prefs_address(&meta.app);
        self.own_app = Some(meta.app.clone());
        self.prefs = Some(address.clone());
        let runtime = host.runtime();
        for ty in &meta.prefs {
            let target = crate::runtime::ResourceRef::root(address.clone(), ty.clone());
            // What the app declared is what every device shows before a person sets it: the
            // settings service's reading replaces it as it arrives.
            let defaults = meta
                .types
                .iter()
                .find(|declared| declared.id == *ty)
                .map(|declared| {
                    let mut held = serde_json::Map::new();
                    for field in &declared.fields {
                        if let Some(default) = &field.default {
                            held.insert(field.name.clone(), default.clone());
                        }
                    }
                    serde_json::Value::Object(held)
                });
            if let Some(defaults) = defaults {
                let _ = runtime.set_reading(&target, &defaults.to_string());
            }
            self.declared.insert(target.id_string());
            self.follow(&target, wake.clone());
        }
    }

    /// Follow `target`, the resource the session publishes under its id.
    fn follow(&mut self, target: &crate::runtime::ResourceRef, wake: tilo_rt::wake::WakeHandle) {
        let Some(source) = published() else {
            return;
        };
        match source.follow(&target.id_string(), wake) {
            Ok(reading) => self.followed.push((target.clone(), reading)),
            Err(why) => tracing::debug!(
                resource = %target.id_string(),
                %why,
                "a platform root is not read here"
            ),
        }
    }

    /// Start `target` reading what it would before its app publishes anything
    /// ([`platform_root_defaults`]), so a view that reads a field of it runs as the app opens,
    /// rather than trapping on a resource with no reading at all. The session's first reading
    /// replaces it. A root the host reads in its own workspace (its members) reads as it is
    /// there.
    fn seed(host: &crate::host::Host, target: &crate::runtime::ResourceRef) {
        if !host.reads_in_workspace(target) {
            let _ = host
                .runtime()
                .set_reading(target, platform_root_defaults(&target.id_string()));
        }
    }

    /// Whether `app` is a platform app this window reads: what its views change is its
    /// resources' actions, asked of the session's services.
    #[must_use]
    pub fn reads(&self, app: &str) -> bool {
        self.apps.contains(app)
    }

    /// Ask the published resource `resource` to run `action` with `arguments`, as the session
    /// serves it, and hand back what it said.
    ///
    /// # Errors
    /// No session's services are reached, or the change was refused or cancelled.
    pub fn invoke(
        &self,
        resource: &str,
        action: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), String> {
        if let Some(app) = self.own_app.as_deref()
            && let Some(why) = refuse_foreign_prefs(resource, app)
        {
            return Err(why);
        }
        published()
            .ok_or_else(|| "nothing reads the platform's resources here".to_owned())?
            .invoke(resource, action, arguments)
    }

    /// Hand the runtime every new reading of the roots followed, and follow the ones its views
    /// read before they were. Whether a reading moved, which runs those views again, and only
    /// them: a reading is parsed once as it arrives, never per frame.
    pub fn read(
        &mut self,
        host: &crate::host::Host,
        reads: impl Fn(&crate::runtime::ResourceRef) -> bool,
    ) -> bool {
        let runtime = host.runtime();
        let mut moved = false;
        // An app reads only its own preferences: a view that names another app's is told so,
        // rather than left waiting for a reading no service will hand it.
        if let Some(app) = self.own_app.clone() {
            let refused: Vec<(String, String)> =
                runtime.take_wanted_if(|id, _| refuse_foreign_prefs(id, &app).is_some());
            for (id, ty) in refused {
                let Some(target) = crate::runtime::ResourceRef::parse_id(&id, ty) else {
                    continue;
                };
                let Some(why) = refuse_foreign_prefs(&id, &app) else {
                    continue;
                };
                let unreadable =
                    serde_json::json!({ tilolang_runtime::store::UNREADABLE: why }).to_string();
                moved |= runtime
                    .set_reading(&target, &unreadable)
                    .unwrap_or_else(|error| {
                        tracing::warn!(%error, resource = %id, "a refused preference read was not said");
                        false
                    });
            }
        }
        for (target, reading) in &self.followed {
            while let Some(result) = reading.latest() {
                match result {
                    Ok(_) if host.reads_in_workspace(target) => {}
                    Ok(reading) => {
                        moved |= runtime.set_reading(target, &reading).unwrap_or_else(|error| {
                            tracing::warn!(%error, resource = %target.id_string(), "a platform reading was refused");
                            false
                        });
                    }
                    Err(why) => {
                        // The device or the service did not answer: the resource could not be
                        // read. A view reads that as it is (`unreadable(r)`) and says so, rather
                        // than reading every field as the empty value its type starts with, which
                        // reads as a device with no battery and no sound. A reading that answers
                        // replaces this one.
                        let unreadable = serde_json::Value::Object(serde_json::Map::from_iter([(
                            tilolang_runtime::store::UNREADABLE.to_owned(),
                            serde_json::Value::String(why.clone()),
                        )]))
                        .to_string();
                        moved |= runtime.set_reading(target, &unreadable).unwrap_or_else(|error| {
                            tracing::warn!(%error, resource = %target.id_string(), "a platform reading was refused");
                            false
                        });
                        tracing::debug!(resource = %target.id_string(), %why, "a platform reading could not be read");
                    }
                }
            }
        }
        // A view read a root before it was followed (a resource this bundle's platform package
        // publishes under no name it declares): it is followed now, and reads as it would start
        // until its reading comes.
        let apps = &self.apps;
        let wanted: Vec<(String, String)> = runtime.take_wanted_of(|app| apps.contains(app));
        let Some(wake) = self.wake.clone() else {
            return moved;
        };
        for (id, ty) in wanted {
            let Some((app, _name)) = ty.split_once('#') else {
                continue;
            };
            if !self.apps.contains(app)
                || self
                    .followed
                    .iter()
                    .any(|(target, _)| target.id_string() == id)
            {
                continue;
            }
            let Some(target) = crate::runtime::ResourceRef::parse_id(&id, ty.clone()) else {
                continue;
            };
            self.follow(&target, wake.clone());
            Self::seed(host, &target);
        }
        // A reading followed because a view read it goes once no view of this window reads it:
        // a detail beside another change, a range another window shows. The session stops
        // publishing it here, and the runtime keeps what was read of it no longer.
        let declared = self.declared.clone();
        self.followed
            .retain(|(target, _)| declared.contains(&target.id_string()) || reads(target));
        moved
    }
}
