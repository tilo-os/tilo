//! What a written view's instance asks of the program that shows it: the packages it reads, the
//! person's words, and their clock, as the window it stands in reads them.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use tilolang_tilo::background::Waker;
use tilolang_tilo::host::Host;
use tilolang_tilo::instance::Platform;

use crate::apps::App;
use crate::wrapper::Wrapper;

/// The platform of one written view: the apps it names, each as the package its published
/// surface reads as, and the wake of the window it is drawn in.
pub(crate) struct ViewPlatform {
    /// The workspace's apps the view names, as packages this runtime does not run.
    pub(crate) apps: Vec<Rc<App>>,
    /// The package that embeds the view's definition, and the description of the reading it
    /// reads the definition from.
    pub(crate) wrapper: Rc<Wrapper>,
    pub(crate) wake: ui::wake::WakeHandle,
    /// Why the packages did not load, when they did not.
    pub(crate) failed: Rc<RefCell<Option<String>>>,
}

impl Platform for ViewPlatform {
    /// A written view runs no app's code: what it names are the workspace's apps, each loaded as
    /// an app this runtime does not run, so the view reads what the app publishes and asks the
    /// app for what it changes.
    fn load(&self, host: &Host) -> u64 {
        let described =
            std::iter::once(&self.wrapper.anchor).chain(self.apps.iter().map(|app| &app.meta));
        for meta in described {
            if let Err(error) = host.runtime().load_foreign(meta.clone()) {
                tracing::warn!(app = meta.app, %error, "a written view cannot name an app");
            }
        }
        if let Err(error) = host.load_compiled(&self.wrapper.compiled) {
            *self.failed.borrow_mut() = Some(error.to_string());
        }
        0
    }

    fn phrase(&self, id: &str, args: &[(String, String)]) -> String {
        tilo_ui_host::i18n::word(id, args, id)
    }

    /// The view speaks the language its window speaks: the words of the apps it names ride along.
    fn add_words(&self, _window: bool, catalogues: &[(String, String)]) {
        tilo_ui_host::i18n::add_shared_catalogues(catalogues);
    }

    fn words_revision(&self) -> u64 {
        tilo_ui_host::i18n::revision()
    }

    fn now(&self) -> String {
        ui::clock::local_now()
            .to_utc()
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    }

    fn zone(&self) -> i64 {
        i64::from(ui::clock::local_now().offset().local_minus_utc())
    }

    fn zone_name(&self) -> Option<String> {
        ui::time::local_zone_name()
    }

    fn shown(&self, kind: &str, value: &str) -> String {
        tilo_ui_host::date::shown_text(kind, value)
    }

    fn clock(&self, hour: u32, minute: u32) -> String {
        ui::clock::hour_minute(hour, minute)
    }

    fn waker(&self) -> Waker {
        let wake = self.wake.clone();
        Arc::new(move || wake.wake())
    }
}
