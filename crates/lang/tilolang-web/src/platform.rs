//! The platform a window with nothing on screen stands on: the packages it loads, the words its
//! window speaks and the clock and zone it reads. `tilolang layout-check` lays its windows out on
//! it, and a browser page runs its apps on it.

use std::sync::Arc;

use std::rc::Rc;
use tilolang_tilo::host::Host;
use tilolang_tilo::instance::Platform;
use tilolang_tilo::tilolang::Compiled;

/// The compiled packages an instance loads: the app and what it uses, the app last; and the
/// packages it uses as metadata: the native apps' it calls (`use so.tilo.session as session`),
/// whose actions run at their own action APIs, and the platform's (the device's, Settings') whose
/// published resources it reads.
pub struct Packages(
    pub Rc<Vec<Compiled>>,
    pub Vec<tilolang_tilo::tilolang::PackageMeta>,
);

impl Platform for Packages {
    fn load(&self, host: &Host) -> u64 {
        // The platform's resources stand as a device that has none of them,
        // read as each declares when nothing is published: a view over the
        // sound or the battery lays out as it does on a computer without
        // them.
        for meta in &self.1 {
            if let Err(error) = host.runtime().load_foreign(meta.clone()) {
                eprintln!("tilolang: a platform package did not load: {error}");
            }
        }
        for (published, ty) in tilolang_tilo::platform_apps::platform_roots() {
            if !self
                .1
                .iter()
                .any(|meta| ty.starts_with(&format!("{}#", meta.app)))
            {
                continue;
            }
            match tilolang_tilo::runtime::ResourceRef::parse_id(published, ty) {
                Some(target) => {
                    if let Err(error) = host.runtime().set_reading(&target, "{}") {
                        eprintln!("tilolang: `{published}` reads nothing: {error}");
                    }
                }
                None => eprintln!("tilolang: `{published}` names no resource"),
            }
        }
        for compiled in self.0.iter() {
            if let Err(error) = host.load_compiled(compiled) {
                eprintln!("tilolang: a package did not load: {error}");
            }
        }
        0
    }

    fn phrase(&self, id: &str, args: &[(String, String)]) -> String {
        let args: Vec<(&str, ui::i18n::ArgValue)> = args
            .iter()
            .map(|(name, value)| (name.as_str(), tilo_app_window::i18n::value_of(value)))
            .collect();
        tilo_app_window::i18n::t_args(id, &args)
    }

    /// The package's words, and the one language its window is laid out in: the person's when the
    /// package ships it, the package's source language otherwise.
    fn add_words(&self, window: bool, catalogues: &[(String, String)]) {
        if window {
            ui::i18n::speak_window_language(catalogues);
        } else {
            tilo_app_window::i18n::add_shared_catalogues(catalogues);
        }
    }

    /// A fixed instant, so every run lays out the same dates: the clock
    /// the capture pins ([`ui::clock::freeze_for_capture`]), which the
    /// days the app reads and the days its window writes both come
    /// from.
    fn now(&self) -> String {
        format!("{:.0}", ui::clock::local_now().timestamp())
    }

    /// The person's zone, as a window knows it: a seed's wall-clock time and every day the window
    /// shows are read in it, so the layout is the one a person in their zone sees.
    fn zone(&self) -> i64 {
        i64::from(ui::clock::local_now().offset().seconds())
    }

    fn zone_name(&self) -> Option<String> {
        ui::time::local_zone_name()
    }

    /// A day or a time of day in a sentence, as the toolkit's lists show them, so a row's line and
    /// a notice carry no stored text.
    fn shown(&self, kind: &str, value: &str) -> String {
        tilo_app_window::date::shown_text(kind, value)
    }

    fn sentence(&self, kind: &str, value: &str) -> String {
        tilo_app_window::date::sentence_text(kind, value)
    }

    /// The time of day as the person reads a clock, in the style they chose.
    fn clock(&self, hour: u32, minute: u32) -> String {
        ui::clock::hour_minute(hour, minute)
    }

    fn waker(&self) -> tilolang_tilo::background::Waker {
        Arc::new(|| {})
    }
}
